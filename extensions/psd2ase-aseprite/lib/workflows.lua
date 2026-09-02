local Workflows = {}

--- Creates import/export workflows from the owning process, dialog, and document boundaries.
function Workflows.new(process, dialogs, documents)
  local state = {}

  --- Imports one PSD/PSB and returns the new sprite plus its optional report text.
  local function import_document(input)
    local roundtrip_marked = process.is_roundtrip_document(process.binary, input)
    local options = dialogs.select_import_options(input, roundtrip_marked)
    if not options then
      return nil
    end
    return process.with_temp_files({"aseprite", "json"}, function(temporary_output, report_filename)
      options.report = report_filename
      process.run_conversion(process.binary, options.input, temporary_output, options)
      local sprite = documents.open_as_unsaved_document(
        temporary_output,
        documents.suggested_output_path(options.input),
        documents.read_imported_active_frame(report_filename))
      return sprite, process.read_file(report_filename)
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
        export_options.compression,
        plugin.preferences.embed_roundtrip_metadata ~= false)
      local bytes = process.read_file(output_filename)
      if bytes == "" then
        error("The converter produced an empty Photoshop document.")
      end
      return bytes, process.read_file(report_filename)
    end)
  end

  --- Executes the menu-driven import workflow and reports failures to the user.
  local function import_from_menu()
    local input = dialogs.select_import_source()
    if not input then
      return
    end
    local success, result = pcall(function()
      local sprite, report = import_document(input)
      if sprite then
        dialogs.show_information_loss(report)
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
    local export_options = dialogs.select_export_options()
    if not export_options then
      return false
    end
    local success, result = pcall(function()
      local bytes, report = create_export_document(ev.sprite, extension, export_options, plugin)
      ev.file:write(bytes)
      ev.file:flush()
      dialogs.show_information_loss(report, "export")
    end)
    if not success then
      dialogs.show_error("PSD export failed", tostring(result))
      return false
    end
    return true
  end

  return {
    import_document = import_document,
    import_from_menu = import_from_menu,
    export_from_menu = export_from_menu,
    save_photoshop_document = save_photoshop_document,
  }
end

return Workflows
