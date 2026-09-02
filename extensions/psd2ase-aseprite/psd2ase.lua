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
  if options.report then
    table.insert(arguments, "--report")
    table.insert(arguments, options.report)
  end

  if options.overwrite then
    table.insert(arguments, "--overwrite")
  end

  if options.preserve_photoshop_metadata then
    table.insert(arguments, "--preserve-photoshop-metadata")
  end

  if options.link_identical_cels and options.layer_association == "auto" then
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

--- Builds the single Rust export command used by the custom file-format saver.
local function build_export_arguments(
  binary,
  input,
  output,
  composite,
  report,
  active_frame_index,
  compression,
  embed_roundtrip_metadata)
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
  if compression ~= nil then
    table.insert(arguments, "--compression")
    table.insert(arguments, compression)
  end
  if not embed_roundtrip_metadata then
    table.insert(arguments, "--roundtrip-metadata")
    table.insert(arguments, "off")
  end
  return arguments
end

--- Shows the export choices shared by custom Save As and the fallback menu.
local function select_export_options()
  local dialog = Dialog{ title="Export PSD/PSB Options" }
  if not dialog then
    show_error("PSD export failed", "Aseprite does not have an available UI.")
    return nil
  end
  dialog:combobox{
    id="compression",
    label="Compression",
    option="ZIP",
    options={"ZIP", "ZIP prediction", "RLE", "Raw"},
  }
  dialog:newrow()
  dialog:button{ id="export", text="Export", focus=true }
  dialog:button{ id="cancel", text="Cancel" }
  dialog:show()
  if not dialog.data.export then
    return nil
  end
  return {
    compression = ({
      ["ZIP"] = "zip",
      ["ZIP prediction"] = "zip-prediction",
      ["RLE"] = "rle",
      ["Raw"] = "raw",
    })[dialog.data.compression] or "zip",
  }
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

--- Runs psd2ase and returns its captured diagnostic output.
local function run_converter(binary, arguments, output)
  if not app.fs.isFile(binary) then
    error("Bundled psd2ase executable was not found: " .. binary)
  end

  local log_filename = temporary_path("log")
  local command = build_command(arguments, log_filename)
  local launch_ok, result, reason, code = pcall(function()
    return os.execute(command)
  end)
  local diagnostics = read_text_file(log_filename)

  if not launch_ok then
    remove_file(log_filename)
    error("Could not launch psd2ase: " .. tostring(result))
  end
  if not command_succeeded(result, reason, code) then
    local detail = diagnostics
    if detail == "" then
      detail = "The converter exited without diagnostic output."
    end
    remove_file(log_filename)
    error(string.format(
      "psd2ase conversion failed (%s).\n\n%s",
      format_exit_status(result, reason, code),
      detail
    ), 0)
  end
  if not app.fs.isFile(output) then
    remove_file(log_filename)
    error("psd2ase reported success but did not create: " .. output)
  end
  remove_file(log_filename)
  return diagnostics
end

--- Runs a converter command that reports diagnostics without creating an output file.
local function run_diagnostics(binary, arguments)
  if not app.fs.isFile(binary) then
    error("Bundled psd2ase executable was not found: " .. binary)
  end
  local log_filename = temporary_path("log")
  local command = build_command(arguments, log_filename)
  local launch_ok, result, reason, code = pcall(function()
    return os.execute(command)
  end)
  local diagnostics = read_text_file(log_filename)
  remove_file(log_filename)
  if not launch_ok then
    error("Could not launch psd2ase: " .. tostring(result))
  end
  if not command_succeeded(result, reason, code) then
    error(string.format(
      "psd2ase inspection failed (%s).\n\n%s",
      format_exit_status(result, reason, code),
      diagnostics ~= "" and diagnostics or "The converter exited without diagnostic output."))
  end
  return diagnostics
end

--- Returns whether a PSD carries a valid converter-owned round-trip marker.
local function is_roundtrip_document(binary, input)
  local success, diagnostics = pcall(function()
    return run_diagnostics(binary, { binary, "inspect", input })
  end)
  return success and diagnostics:find("roundtrip metadata: true", 1, true) ~= nil
end

--- Runs the PSD import subcommand through the common converter launcher.
local function run_conversion(binary, input, output, options)
  if not app.fs.isFile(input) then
    error("PSD input file was not found: " .. input)
  end
  return run_converter(binary, build_arguments(binary, input, output, options), output)
end

--- Runs the Aseprite export subcommand through the common converter launcher.
local function run_export_conversion(
  binary,
  input,
  output,
  composite,
  report,
  active_frame_index,
  compression,
  embed_roundtrip_metadata)
  if not app.fs.isFile(input) or not app.fs.isFile(composite) then
    error("Aseprite export snapshots were not created.")
  end
  return run_converter(
    binary,
    build_export_arguments(
      binary,
      input,
      output,
      composite,
      report,
      active_frame_index,
      compression,
      embed_roundtrip_metadata),
    output)
end

--- Shows a single alert containing an operation failure.
local function split_message_lines(message)
  local normalized = tostring(message):gsub("\r\n", "\n"):gsub("\r", "\n")
  local lines = {}
  for line in (normalized .. "\n"):gmatch("(.-)\n") do
    table.insert(lines, line)
  end
  if #lines == 0 then
    table.insert(lines, "")
  end
  return lines
end

--- Shows an operation failure with one Aseprite alert label per line.
local function show_error(title, message)
  local lines = split_message_lines(message)
  print(title .. ":")
  for _, line in ipairs(lines) do
    print(line)
  end
  app.alert{ title=title, text=lines }
end

--- Saves one structured compatibility report after an explicit export choice.
local function save_information_loss_report(raw, operation)
  local operation_title = operation:gsub("^%l", string.upper)
  local chooser = Dialog{ title="Export PSD " .. operation_title .. " Report" }
  if not chooser then
    show_error("PSD " .. operation .. " report", "Aseprite does not have an available UI.")
    return
  end
  chooser:file{ id="destination", label="Report", save=true, entry=false, filetypes={"json"} }
  chooser:button{ id="save", text="Save", focus=true }
  chooser:button{ id="cancel", text="Cancel" }
  chooser:show()
  if not chooser.data.save then return end
  local saved = chooser.data.destination
  if saved and saved ~= "" then
    local file = io.open(saved, "wb")
    if file then
      file:write(raw)
      file:close()
    else
      show_error("PSD " .. operation .. " report", "Could not save the report.")
    end
  end
end

--- Shows structured compatibility losses and offers an explicit report export.
local function show_information_loss(report_filename, operation)
  operation = operation or "import"
  local operation_title = operation:gsub("^%l", string.upper)
  local raw = read_text_file(report_filename)
  if raw == "" then return end
  local ok, report, losses = pcall(function()
    local decoded = json.decode(raw)
    if decoded.schema_version ~= 1 and decoded.schema_version ~= 2 and decoded.schema_version ~= 3 then
      return nil, nil
    end
    local decoded_losses = decoded.losses or {}
    local loss_count = #decoded_losses
    for index = 1, math.min(loss_count, 8) do
      if decoded_losses[index] == nil then
        error("report losses must be an array")
      end
    end
    return decoded, decoded_losses
  end)
  if not ok or report == nil then
    show_error("PSD " .. operation .. " report", "The converter produced an unreadable or unsupported report.")
    return
  end
  local visible_losses = {}
  for _, loss in ipairs(losses) do
    table.insert(visible_losses, loss)
  end
  losses = visible_losses
  local loss_count = #losses
  if loss_count == 0 then return end
  local lines = {"Some PSD information could not be preserved:"}
  for index = 1, math.min(loss_count, 8) do
    local loss = losses[index]
    local occurrence_count = loss.count or 0
    local locations = loss.locations or {}
    table.insert(lines, string.format(
      "%s: %s (%d)",
      loss.code or "unknown",
      loss.disposition or "unknown",
      occurrence_count))
    for _, location in ipairs(locations) do
      local path = location.path or ""
      if path == "" then
        path = "document"
      end
      local qualifiers = {}
      if location.frame_index ~= nil then
        table.insert(qualifiers, string.format("Frame %d", location.frame_index + 1))
      end
      if location.layer_id ~= nil then
        table.insert(qualifiers, string.format("Layer ID: %d", location.layer_id))
      end
      if #qualifiers > 0 then
        path = path .. " (" .. table.concat(qualifiers, ", ") .. ")"
      end
      table.insert(lines, "  " .. path)
    end
    if occurrence_count > #locations then
      table.insert(lines, string.format(
        "  ... and %d more occurrences not listed",
        occurrence_count - #locations))
    end
  end
  if loss_count > 8 then
    table.insert(lines, string.format("... and %d more entries", loss_count - 8))
  end
  local dialog = Dialog{ title="PSD " .. operation_title .. " Information Loss" }
  for index, line in ipairs(lines) do
    if index > 1 then
      dialog:newrow()
    end
    dialog:label{ id="summary_" .. index, text=line }
  end
  dialog:newrow()
  dialog:button{ id="export", text="Export Full Report..." }
  dialog:button{ id="ok", text="OK", focus=true }
  dialog:show()
  if dialog.data.export then
    save_information_loss_report(raw, operation)
  end
end

--- Returns the compact label shown for the current jitter settings.
local function jitter_summary(options)
  if options.jitter_mode == "off" then
    return "Off…"
  end
  local mode = options.jitter_mode:gsub("^%l", string.upper)
  local kind = options.jitter_kind:gsub("^%l", string.upper)
  return string.format("%s · %s…", mode, kind)
end

--- Opens the modal jitter settings dialog and returns committed values.
local function select_jitter_options(parent_dialog, initial, automatic)
  local selected = {
    jitter_mode = initial.jitter_mode or "off",
    jitter_kind = initial.jitter_kind or "alpha",
    jitter_profile = initial.jitter_profile or "conservative",
  }
  if not automatic then
    selected.jitter_kind = "alpha"
  end

  local dialog = Dialog{ title="Jitter Repair", parent=parent_dialog }
  if not dialog then
    return nil
  end

  --- Keeps the dependent jitter controls aligned with the selected mode.
  local function update_controls()
    local current = dialog.data
    local enabled = current.jitter_mode ~= "off"
    dialog:modify{ id="jitter_kind", enabled=enabled }
    dialog:modify{ id="jitter_profile", enabled=enabled }
  end

  dialog:combobox{
    id="jitter_mode",
    label="Mode",
    option=selected.jitter_mode,
    options={"off", "report", "repair"},
    onchange=update_controls,
  }
  dialog:combobox{
    id="jitter_kind",
    label="Kind",
    option=selected.jitter_kind,
    options=automatic and {"alpha", "color", "all"} or {"alpha"},
    enabled=false,
  }
  dialog:combobox{
    id="jitter_profile",
    label="Profile",
    option=selected.jitter_profile,
    options={"conservative", "balanced"},
    enabled=false,
  }
  update_controls()
  dialog:button{ id="apply", text="Apply", focus=true }
  dialog:button{ id="cancel", text="Cancel" }
  dialog:show()

  local data = dialog.data
  if not data.apply then
    return nil
  end
  return {
    jitter_mode = data.jitter_mode,
    jitter_kind = data.jitter_kind,
    jitter_profile = data.jitter_profile,
  }
end

--- Returns the non-interactive defaults used when Aseprite opens a PSD directly.
local function default_import_options(roundtrip_marked)
  return {
    overwrite = true,
    layer_association = roundtrip_marked and "auto" or "preserve",
    link_identical_cels = false,
    jitter_mode = "off",
    jitter_kind = "alpha",
    jitter_profile = "conservative",
    preserve_photoshop_metadata = false,
    association_strategy = "compact",
    z_order = "stable",
    stable_order = "consensus",
    uncertain_layers = "group",
  }
end

--- Shows the complete PSD import dialog and returns the selected options.
local function select_import_options(input_filename, roundtrip_marked)
  local dialog = Dialog{ title="Import PSD" }
  local defaults = default_import_options(roundtrip_marked)
  local jitter_options = {
    jitter_mode = defaults.jitter_mode,
    jitter_kind = defaults.jitter_kind,
    jitter_profile = defaults.jitter_profile,
  }
  if not dialog then
    show_error("PSD to Aseprite", "Aseprite does not have an available UI.")
    return nil
  end
  if input_filename then
    dialog:label{ id="input_summary", label="PSD", text=app.fs.fileTitle(input_filename) }
    if roundtrip_marked then
      dialog:label{ id="roundtrip_summary", text="Round-trip metadata detected; auto association is enabled by default." }
    end
  else
    dialog:file{
      id="input",
      label="PSD",
      title="Select Photoshop document",
      open=true,
      entry=true,
      filetypes={"psd", "psb"},
    }
  end
  --- Keeps advanced association controls aligned with the selected mode.
  local function update_option_controls()
    local current = dialog.data
    local automatic = current.layer_association == "auto"
    dialog:modify{ id="association_strategy", enabled=automatic }
    dialog:modify{ id="z_order", enabled=automatic }
    dialog:modify{ id="stable_order", enabled=automatic }
    dialog:modify{ id="link_identical_cels", enabled=automatic }
    if not automatic and current.link_identical_cels then
      dialog:modify{ id="link_identical_cels", selected=false }
    end
    dialog:modify{
      id="uncertain_layers",
      enabled=automatic and current.association_strategy == "conservative",
    }
    if not automatic and jitter_options.jitter_kind ~= "alpha" then
      jitter_options.jitter_kind = "alpha"
      dialog:modify{ id="jitter_settings", text=jitter_summary(jitter_options) }
    end
  end
  dialog:combobox{
    id="layer_association",
    label="Layer association",
    option=defaults.layer_association,
    options={"preserve", "auto"},
    onchange=update_option_controls,
  }
  dialog:check{
    id="link_identical_cels",
    label="Linked cels",
    text="Link identical cels",
    selected=defaults.link_identical_cels,
    enabled=false,
  }
  dialog:check{
    id="preserve_photoshop_metadata",
    label="Photoshop metadata",
    text="Preserve metadata for PSD round-trip",
    selected=defaults.preserve_photoshop_metadata,
  }
  dialog:combobox{
    id="association_strategy",
    label="Association strategy",
    option=defaults.association_strategy,
    options={"compact", "conservative"},
    enabled=false,
    onchange=update_option_controls,
  }
  dialog:combobox{
    id="z_order",
    label="Z-order",
    option=defaults.z_order,
    options={"stable", "auto"},
    enabled=false,
  }
  dialog:combobox{
    id="stable_order",
    label="Stable order",
    option=defaults.stable_order,
    options={"consensus", "anchor", "strict"},
    enabled=false,
  }
  dialog:combobox{
    id="uncertain_layers",
    label="Uncertain layers",
    option=defaults.uncertain_layers,
    options={"group", "flat"},
    enabled=false,
  }
  dialog:newrow()
  dialog:button{
    id="jitter_settings",
    label="Jitter repair",
    text=jitter_summary(jitter_options),
    hexpand=false,
    onclick=function()
      local current = dialog.data
      local committed = select_jitter_options(
        dialog,
        jitter_options,
        current.layer_association == "auto")
      if committed then
        jitter_options = committed
        dialog:modify{ id="jitter_settings", text=jitter_summary(jitter_options) }
      end
    end,
  }
  update_option_controls()
  dialog:newrow()
  dialog:button{ id="import", text="Import", focus=true }
  dialog:button{ id="cancel", text="Cancel" }
  dialog:show()
  local data = dialog.data
  data.input = input_filename or data.input
  if not data.import or not data.input or data.input == "" then
    return nil
  end
  data.overwrite = true
  data.jitter_mode = jitter_options.jitter_mode
  data.jitter_kind = jitter_options.jitter_kind
  data.jitter_profile = jitter_options.jitter_profile
  return data
end

--- Returns the native Save As suggestion for an imported PSD.
local function suggested_output_path(input)
  return app.fs.joinPath(app.fs.filePath(input), app.fs.fileTitle(input) .. ".aseprite")
end

--- Reads the optional source active frame from a temporary conversion report.
local function read_imported_active_frame(report_filename)
  local raw = read_text_file(report_filename)
  if raw == "" then
    return
  end
  local ok, report = pcall(json.decode, raw)
  if not ok or type(report) ~= "table" then
    return
  end
  local frame_index = report.active_frame_index
  if type(frame_index) ~= "number" or frame_index < 0 or frame_index % 1 ~= 0 then
    return
  end
  return frame_index
end

--- Applies a temporary imported Photoshop active frame to the Aseprite UI.
local function apply_imported_active_frame(sprite, frame_index)
  if type(frame_index) ~= "number" then
    return
  end
  if frame_index < 0 or frame_index >= #sprite.frames then
    return
  end
  app.sprite = sprite
  app.frame = frame_index + 1
end

--- Returns the current sprite frame as a zero-based export index.
local function current_frame_index(sprite)
  if not sprite or app.sprite ~= sprite then
    error("The PSD export source is not the active Aseprite sprite.")
  end
  local frame = app.frame
  local frame_number
  if type(frame) == "number" then
    frame_number = frame
  elseif frame ~= nil then
    local ok, value = pcall(function()
      return frame.frameNumber
    end)
    if ok then
      frame_number = value
    end
  end
  if type(frame_number) ~= "number" or frame_number % 1 ~= 0 then
    error("Aseprite did not provide a numeric current frame.")
  end
  local frame_index = frame_number - 1
  if frame_index < 0 or frame_index >= #sprite.frames then
    error("The current Aseprite frame is outside the sprite timeline.")
  end
  return frame_index
end

--- Opens and duplicates a converted file as an unassociated, modified document.
local function open_as_unsaved_document(filename, suggested_filename, active_frame_index)
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
    apply_imported_active_frame(sprite, active_frame_index)
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

--- Closes an isolated sprite copy without turning cleanup into an export error.
local function close_sprite(sprite)
  if sprite then
    pcall(function() sprite:close() end)
  end
end

--- Saves original and flattened isolated snapshots without mutating the source sprite.
local function create_export_snapshots(source, original_filename, composite_filename)
  local original_copy
  local composite_copy
  local success, result = pcall(function()
    original_copy = Sprite(source)
    composite_copy = Sprite(source)
    if not original_copy or not composite_copy then
      error("Aseprite could not create isolated export copies.")
    end
    if not original_copy:saveCopyAs(original_filename) then
      error("Aseprite could not save the isolated original snapshot.")
    end
    composite_copy:flatten()
    if not composite_copy:saveCopyAs(composite_filename) then
      error("Aseprite could not save the isolated flattened snapshot.")
    end
  end)
  close_sprite(original_copy)
  close_sprite(composite_copy)
  local restored, restore_error = pcall(function()
    app.sprite = source
  end)
  if not restored then
    error("Aseprite could not restore the source document after snapshot cleanup: "
      .. tostring(restore_error), 0)
  end
  if not success then
    error(result, 0)
  end
end

--- Writes binary export bytes to a user-selected destination.
local function write_binary_file(filename, bytes)
  local file = io.open(filename, "wb")
  if not file then
    error("Could not open the PSD export destination for writing: " .. filename)
  end
  file:write(bytes)
  file:close()
end

--- Exports one sprite into a verified temporary PSD/PSB and commits it to ev.file.
local function save_photoshop_document(binary, ev, plugin)
  if not binary then
    show_error("PSD export failed", "This extension has no converter for the current platform.")
    return false
  end
  local extension = (app.fs.fileExtension(ev.filename) or ""):lower()
  if extension ~= "psd" and extension ~= "psb" then
    show_error("PSD export failed", "The destination must use a .psd or .psb extension.")
    return false
  end
  local export_options = select_export_options()
  if not export_options then
    return false
  end
  local original_filename = temporary_path("aseprite")
  local composite_filename = temporary_path("aseprite")
  local output_filename = temporary_path(extension)
  local report_filename = temporary_path("json")
  local success, result = pcall(function()
    local active_frame_index = current_frame_index(ev.sprite)
    create_export_snapshots(ev.sprite, original_filename, composite_filename)
    run_export_conversion(
      binary,
      original_filename,
      output_filename,
      composite_filename,
      report_filename,
      active_frame_index,
      export_options.compression,
      plugin.preferences.embed_roundtrip_metadata ~= false)
    local bytes = read_text_file(output_filename)
    if bytes == "" then
      error("The converter produced an empty Photoshop document.")
    end
    ev.file:write(bytes)
    ev.file:flush()
    show_information_loss(report_filename, "export")
  end)
  remove_file(original_filename)
  remove_file(composite_filename)
  remove_file(output_filename)
  remove_file(report_filename)
  if not success then
    show_error("PSD export failed", tostring(result))
    return false
  end
  return true
end

--- Shows the fallback PSD/PSB destination picker for hosts without file-format registration.
local function select_export_destination()
  local dialog = Dialog{ title="Export PSD/PSB" }
  if not dialog then
    show_error("PSD export failed", "Aseprite does not have an available UI.")
    return nil
  end
  dialog:file{
    id="destination",
    label="PSD/PSB",
    title="Select Photoshop export destination",
    save=true,
    entry=true,
    filetypes={"psd", "psb"},
  }
  dialog:button{ id="export", text="Export", focus=true }
  dialog:button{ id="cancel", text="Cancel" }
  dialog:show()
  if not dialog.data.export then
    return nil
  end
  local destination = dialog.data.destination
  if not destination or destination == "" then
    return nil
  end
  local extension = (app.fs.fileExtension(destination) or ""):lower()
  if extension ~= "psd" and extension ~= "psb" then
    show_error("PSD export failed", "The destination must use a .psd or .psb extension.")
    return nil
  end
  return destination
end

--- Exports the active sprite through the fallback PSD/PSB menu command.
local function export_from_menu(binary, plugin)
  if not binary then
    show_error("PSD export failed", "This extension has no converter for the current platform.")
    return
  end
  if not app.sprite then
    show_error("PSD export failed", "There is no active Aseprite sprite to export.")
    return
  end
  local destination = select_export_destination()
  if not destination then
    return
  end
  local export_options = select_export_options()
  if not export_options then
    return
  end
  local extension = (app.fs.fileExtension(destination) or ""):lower()
  local original_filename = temporary_path("aseprite")
  local composite_filename = temporary_path("aseprite")
  local output_filename = temporary_path(extension)
  local report_filename = temporary_path("json")
  local success, result = pcall(function()
    local active_frame_index = current_frame_index(app.sprite)
    create_export_snapshots(app.sprite, original_filename, composite_filename)
    run_export_conversion(
      binary,
      original_filename,
      output_filename,
      composite_filename,
      report_filename,
      active_frame_index,
      export_options.compression,
      plugin.preferences.embed_roundtrip_metadata ~= false)
    local bytes = read_text_file(output_filename)
    if bytes == "" then
      error("The converter produced an empty Photoshop document.")
    end
    write_binary_file(destination, bytes)
    show_information_loss(report_filename, "export")
  end)
  remove_file(original_filename)
  remove_file(composite_filename)
  remove_file(output_filename)
  remove_file(report_filename)
  if not success then
    show_error("PSD export failed", tostring(result))
  end
end

--- Selects a PSD/PSB source before showing its import settings.
local function select_import_source()
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
    filetypes={"psd", "psb"},
  }
  dialog:button{ id="select", text="Next", focus=true }
  dialog:button{ id="cancel", text="Cancel" }
  dialog:show()
  if not dialog.data.select or not dialog.data.input or dialog.data.input == "" then
    return nil
  end
  return dialog.data.input
end

--- Executes the menu-driven import workflow through a temporary output file.
local function import_from_menu(binary)
  local input = select_import_source()
  if not input then
    return
  end
  local options = select_import_options(input, is_roundtrip_document(binary, input))
  if not options then
    return
  end
  local temporary_output = temporary_path("aseprite")
  options.report = temporary_path("json")
  local success, result = pcall(function()
    run_conversion(binary, options.input, temporary_output, options)
    open_as_unsaved_document(
      temporary_output,
      suggested_output_path(options.input),
      read_imported_active_frame(options.report))
    show_information_loss(options.report)
  end)
  remove_file(temporary_output)
  remove_file(options.report)
  if not success then
    show_error("PSD import failed", tostring(result))
  end
end

--- Shows and persists the PSD round-trip metadata preference for this extension.
local function show_roundtrip_settings(plugin)
  local dialog = Dialog{ title="PSD to Aseprite Settings" }
  if not dialog then
    show_error("PSD to Aseprite", "Aseprite does not have an available UI.")
    return
  end
  dialog:check{
    id="embed_roundtrip_metadata",
    label="PSD round-trip",
    text="Embed invisible PSD round-trip metadata",
    selected=plugin.preferences.embed_roundtrip_metadata ~= false,
  }
  dialog:label{
    text="Stores only version, logical layer IDs, and cel relationships; no paths or user data.",
  }
  dialog:label{
    text="Disable this only if you do not want automatic association when reopening exported PSDs.",
  }
  dialog:newrow()
  dialog:button{ id="apply", text="Apply", focus=true }
  dialog:button{ id="cancel", text="Cancel" }
  dialog:show()
  if dialog.data.apply then
    plugin.preferences.embed_roundtrip_metadata = dialog.data.embed_roundtrip_metadata == true
  end
end

--- Registers the explicit PSD import command.
function init(plugin)
  local binary = converter_path(plugin)
  if plugin.preferences.embed_roundtrip_metadata == nil then
    plugin.preferences.embed_roundtrip_metadata = true
  end
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
  plugin:newCommand{
    id="Psd2aseExport",
    title="Export PSD/PSB...",
    group="file_export",
    onclick=function()
      export_from_menu(binary, plugin)
    end,
  }
  plugin:newCommand{
    id="Psd2aseSettings",
    title="PSD to Aseprite Settings...",
    group="file_export",
    onclick=function()
      show_roundtrip_settings(plugin)
    end,
  }
  plugin:newFileFormat{
    name="Photoshop Document (PSD/PSB)",
    extensions={"psd", "psb"},
    binary=true,
    onsave=function(ev)
      return save_photoshop_document(binary, ev, plugin)
    end,
    onload=function(ev)
      if not binary then
        error("This extension has no converter for the current platform.")
      end
      local temporary_output = temporary_path("aseprite")
      local report_filename = temporary_path("json")
      local options = select_import_options(
        ev.filename,
        is_roundtrip_document(binary, ev.filename))
      if not options then
        remove_file(temporary_output)
        remove_file(report_filename)
        return nil
      end
      options.report = report_filename
      local success, result = pcall(function()
        run_conversion(binary, ev.filename, temporary_output, options)
        return open_as_unsaved_document(
          temporary_output,
          suggested_output_path(ev.filename),
          read_imported_active_frame(report_filename))
      end)
      if success then show_information_loss(report_filename) end
      remove_file(temporary_output)
      remove_file(report_filename)
      if not success then
        error(result, 0)
      end
      return result
    end,
  }
  if not binary or not app.fs.isFile(binary) then
    print("psd2ase: no supported bundled executable was found")
  end
end
