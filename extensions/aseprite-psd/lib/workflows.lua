local Workflows = {}

--- Creates import/export workflows from the owning process, dialog, and document boundaries.
function Workflows.new(process, dialogs, documents)
  local state = {
    export_sessions = setmetatable({}, { __mode = "k" }),
  }

  --- Converts one PSD/PSB and opens it using the supplied document strategy.
  local function convert_document(input, options, opener)
    return process.with_temp_files({"aseprite", "json"}, function(temporary_output, report_filename)
      options.report = report_filename
      local diagnostics, exit_code = process.run_conversion(
        process.binary, options.input, temporary_output, options)
      if exit_code == 4 then
        return nil, { recovery_required = true, diagnostics = diagnostics }
      end
      local sprite = opener(
        temporary_output,
        documents.suggested_output_path(options.input),
        documents.read_imported_active_frame(report_filename))
      return sprite, process.read_file(report_filename)
    end)
  end

  --- Retries a conversion after asking the user to choose a recovery strategy.
  local function convert_with_recovery(input, options, opener)
    local sprite, report = convert_document(input, options, opener)
    if not report or not report.recovery_required then
      return sprite, report
    end
    local strategy, status = dialogs.select_roundtrip_recovery()
    if not strategy then
      return nil, { cancelled = status == "cancelled", reason = status }
    end
    options.layer_association = strategy
    if strategy == "auto" then
      options.use_roundtrip_metadata = false
    end
    return convert_document(input, options, opener)
  end

  --- Imports one PSD/PSB through the interactive menu as an unassociated copy.
  local function import_document(input, plugin)
    local options, status = dialogs.select_import_options(input, plugin and plugin.preferences)
    if not options then
      return nil, { cancelled = status == "cancelled", reason = status }
    end
    options.input = input
    return convert_with_recovery(input, options, documents.open_as_imported_copy)
  end

  --- Loads one PSD/PSB through the native file-format path after choosing import options.
  local function load_photoshop_document(input, plugin)
    local options, status = dialogs.select_import_options(input, plugin and plugin.preferences)
    if not options then
      return nil, { cancelled = status == "cancelled", reason = status }
    end
    options.input = input
    return convert_with_recovery(input, options, function(filename, _, active_frame_index)
      return documents.open_for_native_load(filename, active_frame_index)
    end)
  end

  --- Exports one active sprite to verified PSD/PSB bytes and returns its report text.
  local function create_export_document(source, extension, export_options, plugin)
    return process.with_temp_files({"aseprite", "aseprite", extension, "json"}, function(
      original_filename,
      composite_filename,
      output_filename,
      report_filename)
      local active_frame_index = documents.current_frame_index(source)
      documents.create_export_snapshots(source, original_filename, composite_filename)
      process.run_export_conversion(
        process.binary,
        original_filename,
        output_filename,
        composite_filename,
        report_filename,
        active_frame_index,
        plugin.preferences.embed_roundtrip_metadata ~= false,
        export_options.include_empty_layers == true)
      local bytes = process.read_file(output_filename)
      if bytes == "" then
        error("The converter produced an empty Photoshop document.")
      end
      return bytes, process.read_file(report_filename)
    end)
  end

  --- Executes the menu-driven import workflow and reports failures to the user.
  local function import_from_menu(plugin)
    local input = dialogs.select_import_source()
    if not input then
      return
    end
    local success, result = pcall(function()
      local sprite, report = import_document(input, plugin)
      if sprite then
        dialogs.show_information_loss(report)
      elseif report and not report.cancelled then
        error("PSD import did not produce a sprite: " .. tostring(report.reason or "unknown error"))
      end
      return sprite
    end)
    if not success then
      dialogs.show_error("PSD import failed", tostring(result))
      return
    end
  end

  --- Executes the fallback PSD/PSB export menu command.
  local function export_from_menu(plugin)
    if not process.binary then
      dialogs.show_error("PSD export failed", "This extension has no converter for the current platform.")
      return
    end
    if not app.sprite then
      dialogs.show_error("PSD export failed", "There is no active Aseprite sprite to export.")
      return
    end
    local destination = dialogs.select_export_destination()
    if not destination then
      return
    end
    local export_options = dialogs.select_export_options()
    if not export_options then
      return
    end
    local extension = (app.fs.fileExtension(destination) or ""):lower()
    local success, result = pcall(function()
      local bytes, report = create_export_document(app.sprite, extension, export_options, plugin)
      process.write_file(destination, bytes)
      dialogs.show_information_loss(report, "export")
    end)
    if not success then
      dialogs.show_error("PSD export failed", tostring(result))
    end
  end

  --- Exports one sprite into a verified temporary PSD/PSB and commits it to ev.file.
  local function save_photoshop_document(ev, plugin)
    if not process.binary then
      dialogs.show_error("PSD export failed", "This extension has no converter for the current platform.")
      return false
    end
    local extension = (app.fs.fileExtension(ev.filename) or ""):lower()
    if extension ~= "psd" and extension ~= "psb" then
      dialogs.show_error("PSD export failed", "The destination must use a .psd or .psb extension.")
      return false
    end
    local session = state.export_sessions[ev.sprite]
    local export_options
    if session and session.filename == ev.filename then
      export_options = {
        include_empty_layers = session.include_empty_layers,
      }
    else
      export_options = dialogs.select_export_options()
    end
    if not export_options then
      return false
    end
    local success, result = pcall(function()
      local bytes, report = create_export_document(ev.sprite, extension, export_options, plugin)
      ev.file:write(bytes)
      ev.file:flush()
      dialogs.show_information_loss(report, "export")
      state.export_sessions[ev.sprite] = {
        filename = ev.filename,
        include_empty_layers = export_options.include_empty_layers == true,
      }
    end)
    if not success then
      dialogs.show_error("PSD export failed", tostring(result))
      return false
    end
    return true
  end

  return {
    import_document = import_document,
    load_photoshop_document = load_photoshop_document,
    import_from_menu = import_from_menu,
    export_from_menu = export_from_menu,
    save_photoshop_document = save_photoshop_document,
  }
end

return Workflows
