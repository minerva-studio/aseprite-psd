local COMMAND_ID = "Psd2aseImport"
local TEMP_SEQUENCE = 0
local BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

--- Returns whether the current Aseprite process is running on Windows.
local function is_windows()
  return app.os.windows
end

--- Returns the bundled converter path for the current supported platform.
local function converter_path(plugin)
  local platform_directory
  local executable
  if app.os.windows and app.os.x64 then
    platform_directory = "windows-x64"
    executable = "psd2ase.exe"
  elseif app.os.linux and app.os.x64 then
    platform_directory = "linux-x64"
    executable = "psd2ase"
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

--- Reads a complete text file and returns an empty string when it is absent.
local function read_text_file(filename)
  local file = io.open(filename, "rb")
  if not file then
    return ""
  end
  local contents = file:read("*a") or ""
  file:close()
  return contents
end

--- Removes a temporary file without turning cleanup into a conversion error.
local function remove_file(filename)
  if filename and app.fs.isFile(filename) then
    os.remove(filename)
  end
end

--- Creates a unique file path below Aseprite's temporary directory.
local function temporary_path(extension)
  TEMP_SEQUENCE = TEMP_SEQUENCE + 1
  local candidate = app.fs.joinPath(
    app.fs.tempPath,
    string.format("psd2ase-%d-%d.%s", os.time(), TEMP_SEQUENCE, extension)
  )
  while app.fs.isFile(candidate) do
    TEMP_SEQUENCE = TEMP_SEQUENCE + 1
    candidate = app.fs.joinPath(
      app.fs.tempPath,
      string.format("psd2ase-%d-%d.%s", os.time(), TEMP_SEQUENCE, extension)
    )
  end
  return candidate
end

--- Returns true for every successful os.execute return representation.
local function command_succeeded(result, reason, code)
  return result == true or result == 0 or (reason == "exit" and code == 0)
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

  if options.overwrite then
    table.insert(arguments, "--overwrite")
  end

  if options.layer_association == "auto" then
    table.insert(arguments, "--layer-association")
    table.insert(arguments, "auto")
    table.insert(arguments, "--association-strategy")
    table.insert(arguments, options.association_strategy)
    table.insert(arguments, "--z-order")
    table.insert(arguments, options.z_order)
    table.insert(arguments, "--stable-order")
    table.insert(arguments, options.stable_order)
    if options.association_strategy == "conservative" then
      table.insert(arguments, "--uncertain-layers")
      table.insert(arguments, options.uncertain_layers)
    end
  end

  return arguments
end

--- Builds an ASCII-only PowerShell launcher for Unicode Windows arguments.
local function build_windows_command(arguments, log_filename)
  local invocation = { "&", quote_powershell_literal(arguments[1]) }
  for index = 2, #arguments do
    table.insert(invocation, quote_powershell_literal(arguments[index]))
  end
  local script = table.concat(invocation, " ")
    .. " *> " .. quote_powershell_literal(log_filename)
    .. "; if ($null -eq $LASTEXITCODE) { exit 1 } else { exit $LASTEXITCODE }"
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

--- Runs psd2ase and returns its captured diagnostic output.
local function run_conversion(binary, input, output, options)
  if not app.fs.isFile(binary) then
    error("Bundled psd2ase executable was not found: " .. binary)
  end
  if not app.fs.isFile(input) then
    error("PSD input file was not found: " .. input)
  end

  local log_filename = temporary_path("log")
  local arguments = build_arguments(binary, input, output, options)
  local command = build_command(arguments, log_filename)
  local launch_ok, result, reason, code = pcall(function()
    return os.execute(command)
  end)
  local diagnostics = read_text_file(log_filename)

  if not launch_ok then
    error("Could not launch psd2ase: " .. tostring(result) .. "\n\nLog: " .. log_filename)
  end
  if not command_succeeded(result, reason, code) then
    local detail = diagnostics
    if detail == "" then
      detail = "The converter exited without diagnostic output."
    end
    error(string.format(
      "psd2ase conversion failed (%s).\n\n%s\n\nLog: %s",
      format_exit_status(result, reason, code),
      detail,
      log_filename
    ))
  end
  if not app.fs.isFile(output) then
    error("psd2ase reported success but did not create: " .. output .. "\n\nLog: " .. log_filename)
  end
  remove_file(log_filename)
  return diagnostics
end

--- Shows a single alert containing an operation failure.
local function show_error(title, message)
  app.alert{ title=title, text=message }
end

--- Shows the complete PSD import dialog and returns the selected options.
local function select_import_options()
  local dialog = Dialog{ title="Import PSD" }
  if not dialog then
    show_error("PSD to Aseprite", "Aseprite does not have an available UI.")
    return nil
  end
  dialog:file{
    id="input",
    label="PSD",
    title="Select Photoshop document",
    open=true,
    entry=true,
    filetypes={"psd"},
  }
  --- Keeps advanced association controls aligned with the selected mode.
  local function update_option_controls()
    local current = dialog.data
    local automatic = current.layer_association == "auto"
    dialog:modify{ id="association_strategy", enabled=automatic }
    dialog:modify{ id="z_order", enabled=automatic }
    dialog:modify{ id="stable_order", enabled=automatic }
    dialog:modify{
      id="uncertain_layers",
      enabled=automatic and current.association_strategy == "conservative",
    }
  end
  dialog:combobox{
    id="layer_association",
    label="Layer association",
    option="preserve",
    options={"preserve", "auto"},
    onchange=update_option_controls,
  }
  dialog:combobox{
    id="association_strategy",
    label="Association strategy",
    option="compact",
    options={"compact", "conservative"},
    enabled=false,
    onchange=update_option_controls,
  }
  dialog:combobox{
    id="z_order",
    label="Z-order",
    option="stable",
    options={"stable", "auto"},
    enabled=false,
  }
  dialog:combobox{
    id="stable_order",
    label="Stable order",
    option="consensus",
    options={"consensus", "anchor", "strict"},
    enabled=false,
  }
  dialog:combobox{
    id="uncertain_layers",
    label="Uncertain layers",
    option="group",
    options={"group", "flat"},
    enabled=false,
  }
  dialog:button{ id="import", text="Import", focus=true }
  dialog:button{ id="cancel", text="Cancel" }
  dialog:show()
  local data = dialog.data
  if not data.import or not data.input or data.input == "" then
    return nil
  end
  data.overwrite = true
  return data
end

--- Returns the native Save As suggestion for an imported PSD.
local function suggested_output_path(input)
  return app.fs.joinPath(app.fs.filePath(input), app.fs.fileTitle(input) .. ".aseprite")
end

--- Opens and duplicates a converted file as an unassociated, modified document.
local function open_as_unsaved_document(filename, suggested_filename)
  local temporary_sprite = app.open(filename)
  if not temporary_sprite then
    error("Aseprite could not open the generated temporary file: " .. filename)
  end

  local sprite
  local success, result = pcall(function()
    sprite = Sprite(temporary_sprite)
    if not sprite then
      error("Aseprite could not duplicate the generated temporary document.")
    end
    sprite.filename = suggested_filename
    app.transaction("Mark imported PSD as modified", function()
      local marker_layer = sprite:newLayer()
      sprite:deleteLayer(marker_layer)
    end)
    if sprite.hasAssociatedFile or not sprite.isModified then
      error("Aseprite did not keep the imported document unassociated and modified.")
    end
  end)
  temporary_sprite:close()
  if not success then
    if sprite then
      sprite:close()
    end
    error(result, 0)
  end
  return sprite
end

--- Executes the menu-driven import workflow through a temporary output file.
local function import_from_menu(binary)
  local options = select_import_options()
  if not options then
    return
  end
  local temporary_output = temporary_path("aseprite")
  local success, result = pcall(function()
    run_conversion(binary, options.input, temporary_output, options)
    open_as_unsaved_document(temporary_output, suggested_output_path(options.input))
  end)
  remove_file(temporary_output)
  if not success then
    show_error("PSD import failed", tostring(result))
  end
end

--- Registers the explicit PSD import command.
function init(plugin)
  local binary = converter_path(plugin)
  plugin:newCommand{
    id=COMMAND_ID,
    title="Import PSD to Aseprite...",
    group="file_import",
    onclick=function()
      if not binary then
        show_error("PSD to Aseprite", "This extension has no binary for the current platform.")
        return
      end
      import_from_menu(binary)
    end,
  }
  if not binary or not app.fs.isFile(binary) then
    print("psd2ase: no supported bundled executable was found")
  end
end
