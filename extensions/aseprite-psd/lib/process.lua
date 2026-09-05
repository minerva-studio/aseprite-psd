local BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

local Process = {}

--- Creates the process boundary for one initialized Aseprite extension instance.
function Process.new(plugin)
  local state = {
    plugin = plugin,
    temporary_sequence = 0,
  }

  --- Returns whether the current Aseprite process is running on Windows.
  local function is_windows()
    return app.os.windows
  end

  --- Returns the bundled converter path for the current supported platform.
  local function converter_path()
    local platform_directory
    local executable
    if app.os.windows and app.os.x64 then
      platform_directory = "windows-x64"
      executable = "aseprite-psd.exe"
    elseif app.os.linux and app.os.x64 then
      platform_directory = "linux-x64"
      executable = "aseprite-psd"
    elseif app.os.macos and app.os.arm64 then
      platform_directory = "macos-arm64"
      executable = "aseprite-psd"
    elseif app.os.macos and app.os.x64 then
      platform_directory = "macos-x64"
      executable = "aseprite-psd"
    else
      return nil
    end
    return app.fs.joinPath(plugin.path, "bin", platform_directory, executable)
  end

  --- Quotes one command-line argument for the current platform shell.
  local function quote_argument(value)
    value = tostring(value)
    if is_windows() then
      return '"' .. value:gsub('"', '\\"') .. '"'
    end
    return "'" .. value:gsub("'", "'\\''") .. "'"
  end

  --- Quotes one literal string for a PowerShell script.
  local function quote_powershell_literal(value)
    return "'" .. tostring(value):gsub("'", "''") .. "'"
  end

  --- Encodes arbitrary bytes as Base64 without external Lua modules.
  local function encode_base64(value)
    local encoded = {}
    local length = #value
    for index = 1, length, 3 do
      local first = value:byte(index)
      local second = value:byte(index + 1)
      local third = value:byte(index + 2)
      local combined = first * 65536 + (second or 0) * 256 + (third or 0)
      local a = math.floor(combined / 262144) % 64
      local b = math.floor(combined / 4096) % 64
      local c = math.floor(combined / 64) % 64
      local d = combined % 64
      table.insert(encoded, BASE64_ALPHABET:sub(a + 1, a + 1))
      table.insert(encoded, BASE64_ALPHABET:sub(b + 1, b + 1))
      table.insert(encoded, second and BASE64_ALPHABET:sub(c + 1, c + 1) or "=")
      table.insert(encoded, third and BASE64_ALPHABET:sub(d + 1, d + 1) or "=")
    end
    return table.concat(encoded)
  end

  --- Reads a complete binary file and returns an empty string when it is absent.
  local function read_file(filename)
    local file = io.open(filename, "rb")
    if not file then
      return ""
    end
    local contents = file:read("*a") or ""
    file:close()
    return contents
  end

  --- Writes binary bytes to a file selected by the owning workflow.
  local function write_file(filename, bytes)
    local file = io.open(filename, "wb")
    if not file then
      error("Could not open the PSD export destination for writing: " .. filename)
    end
    file:write(bytes)
    file:close()
  end

  --- Removes a temporary file without turning cleanup into a conversion error.
  local function remove_file(filename)
    if filename and app.fs.isFile(filename) then
      os.remove(filename)
    end
  end

  --- Creates a unique file path below Aseprite's temporary directory.
  local function temporary_path(extension)
    state.temporary_sequence = state.temporary_sequence + 1
    local candidate = app.fs.joinPath(
      app.fs.tempPath,
      string.format("aseprite-psd-%d-%d.%s", os.time(), state.temporary_sequence, extension)
    )
    while app.fs.isFile(candidate) do
      state.temporary_sequence = state.temporary_sequence + 1
      candidate = app.fs.joinPath(
        app.fs.tempPath,
        string.format("aseprite-psd-%d-%d.%s", os.time(), state.temporary_sequence, extension)
      )
    end
    return candidate
  end

  --- Returns true for every successful os.execute return representation.
  local function command_succeeded(result, reason, code)
    return result == true or result == 0 or (reason == "exit" and code == 0)
  end

  --- Returns whether a converter process requested round-trip recovery.
  local function is_recovery_exit(result, reason, code)
    return result == 4 or (reason == "exit" and code == 4)
  end

  --- Builds the converter argument list for the selected conversion policy.
  local function build_arguments(binary, input, output, options)
    local arguments = {
      binary,
      "convert",
      input,
      "--output",
      output,
    }
    if options.report then
      table.insert(arguments, "--report")
      table.insert(arguments, options.report)
    end
    if options.overwrite then
      table.insert(arguments, "--overwrite")
    end
    table.insert(arguments, "--frame-source")
    table.insert(arguments, options.frame_source or "auto")
    if options.preserve_photoshop_metadata then
      table.insert(arguments, "--preserve-photoshop-metadata")
    end
    local roundtrip = options.layer_association == "roundtrip"
      or (options.layer_association == "auto" and options.use_roundtrip_metadata == true)
    if options.link_identical_cels and options.layer_association == "auto" and not roundtrip then
      table.insert(arguments, "--linked-cels")
      table.insert(arguments, "identical")
    end
    if options.jitter_mode and options.jitter_mode ~= "off" then
      table.insert(arguments, "--jitter-mode")
      table.insert(arguments, options.jitter_mode)
      table.insert(arguments, "--jitter-kind")
      table.insert(arguments, options.jitter_kind)
      table.insert(arguments, "--jitter-profile")
      table.insert(arguments, options.jitter_profile)
    end
    if roundtrip then
      table.insert(arguments, "--layer-association")
      table.insert(arguments, "roundtrip")
    elseif options.layer_association == "auto" then
      table.insert(arguments, "--layer-association")
      table.insert(arguments, "auto")
      table.insert(arguments, "--association-strategy")
      local association_strategy = options.association_strategy
      if association_strategy == "Feature tracks" then
        association_strategy = "feature"
      end
      table.insert(arguments, association_strategy)
      table.insert(arguments, "--z-order")
      table.insert(arguments, options.z_order)
      table.insert(arguments, "--stable-order")
      table.insert(arguments, options.stable_order)
      if association_strategy == "conservative" then
        table.insert(arguments, "--uncertain-layers")
        table.insert(arguments, options.uncertain_layers)
      end
    end
    return arguments
  end

  --- Builds the single Rust export command used by the custom file-format saver.
  local function build_export_arguments(
    binary,
    input,
    output,
    composite,
    report,
    active_frame_index,
    embed_roundtrip_metadata,
    include_empty_layers)
    local arguments = {
      binary,
      "export",
      input,
      "--output",
      output,
      "--composite",
      composite,
      "--report",
      report,
    }
    if active_frame_index ~= nil then
      table.insert(arguments, "--active-frame-index")
      table.insert(arguments, active_frame_index)
    end
    if not embed_roundtrip_metadata then
      table.insert(arguments, "--roundtrip-metadata")
      table.insert(arguments, "off")
    end
    table.insert(arguments, "--empty-layers")
    table.insert(arguments, include_empty_layers == true and "include" or "omit")
    return arguments
  end

  --- Builds an ASCII-only PowerShell launcher for Unicode Windows arguments.
  local function build_windows_command(arguments, log_filename)
    local invocation = { "&", quote_powershell_literal(arguments[1]) }
    for index = 2, #arguments do
      table.insert(invocation, quote_powershell_literal(arguments[index]))
    end
    local command = table.concat(invocation, " ")
    local script = "$captured = @(" .. command .. " 2>&1); "
      .. "$exitCode = $LASTEXITCODE; "
      .. "$text = ($captured | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine; "
      .. "[IO.File]::WriteAllText(" .. quote_powershell_literal(log_filename)
      .. ", $text, [Text.UTF8Encoding]::new($false)); "
      .. "if ($null -eq $exitCode) { exit 1 } else { exit $exitCode }"
    local payload = encode_base64(script)
    return "powershell.exe -NoLogo -NoProfile -NonInteractive -Command "
      .. quote_argument("$s=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('"
        .. payload .. "')); Invoke-Expression $s")
  end

  --- Builds the platform command used to launch the converter.
  local function build_command(arguments, log_filename)
    if is_windows() then
      return build_windows_command(arguments, log_filename)
    end
    local quoted = {}
    for _, argument in ipairs(arguments) do
      table.insert(quoted, quote_argument(argument))
    end
    return table.concat(quoted, " ") .. " > " .. quote_argument(log_filename) .. " 2>&1"
  end

  --- Formats the complete os.execute result for actionable diagnostics.
  local function format_exit_status(result, reason, code)
    return string.format(
      "result=%s, reason=%s, code=%s",
      tostring(result),
      tostring(reason),
      tostring(code)
    )
  end

  --- Runs one converter command, captures diagnostics, and optionally checks its output.
  local function run_process(binary, arguments, output, operation)
    if not app.fs.isFile(binary) then
      error("Bundled aseprite-psd executable was not found: " .. tostring(binary))
    end
    local log_filename = temporary_path("log")
    local command = build_command(arguments, log_filename)
    local launch_ok, result, reason, code = pcall(function()
      return os.execute(command)
    end)
    local diagnostics = read_file(log_filename)
    if not launch_ok then
      remove_file(log_filename)
      error("Could not launch aseprite-psd: " .. tostring(result))
    end
    if not command_succeeded(result, reason, code) then
      local detail = diagnostics
      if detail == "" then
        detail = "The converter exited without diagnostic output."
      end
      if is_recovery_exit(result, reason, code) then
        remove_file(log_filename)
        return detail, 4
      end
      remove_file(log_filename)
      error(string.format(
        "aseprite-psd %s failed (%s).\n\n%s",
        operation or "command",
        format_exit_status(result, reason, code),
        detail
      ), 0)
    end
    if output and not app.fs.isFile(output) then
      remove_file(log_filename)
      error("aseprite-psd reported success but did not create: " .. output)
    end
    remove_file(log_filename)
    return diagnostics
  end

  --- Runs an inspection command and returns its diagnostics without requiring output.
  local function run_diagnostics(binary, arguments)
    return run_process(binary, arguments, nil, "inspection")
  end

  --- Runs the PSD import subcommand through the common converter launcher.
  local function run_conversion(binary, input, output, options)
    if not app.fs.isFile(input) then
      error("PSD input file was not found: " .. input)
    end
    return run_process(binary, build_arguments(binary, input, output, options), output, "conversion")
  end

  --- Runs the Aseprite export subcommand through the common converter launcher.
  local function run_export_conversion(
    binary,
    input,
    output,
    composite,
    report,
    active_frame_index,
    embed_roundtrip_metadata,
    include_empty_layers)
    if not app.fs.isFile(input) or not app.fs.isFile(composite) then
      error("Aseprite export snapshots were not created.")
    end
    return run_process(
      binary,
      build_export_arguments(
        binary,
        input,
        output,
        composite,
        report,
        active_frame_index,
        embed_roundtrip_metadata,
        include_empty_layers),
      output,
      "conversion")
  end

  --- Runs a callback with uniquely named temporary files and always removes them afterward.
  local function with_temp_files(extensions, callback)
    local paths = {}
    for _, extension in ipairs(extensions) do
      table.insert(paths, temporary_path(extension))
    end
    local results = { pcall(function() return callback(table.unpack(paths)) end) }
    for _, path in ipairs(paths) do
      remove_file(path)
    end
    if not results[1] then
      error(results[2], 0)
    end
    table.remove(results, 1)
    return table.unpack(results)
  end

  return {
    binary = converter_path(),
    build_arguments = build_arguments,
    build_export_arguments = build_export_arguments,
    read_file = read_file,
    write_file = write_file,
    remove_file = remove_file,
    temporary_path = temporary_path,
    with_temp_files = with_temp_files,
    run_conversion = run_conversion,
    run_export_conversion = run_export_conversion,
  }
end

return Process
