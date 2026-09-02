local Dialogs = {}

--- Creates the UI boundary for one initialized Aseprite extension instance.
function Dialogs.new(process)
  local state = {}

  --- Splits an error message into lines accepted by Aseprite's alert widget.
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
      local success = pcall(function()
        process.write_file(saved, raw)
      end)
      if not success then
        show_error("PSD " .. operation .. " report", "Could not save the report.")
      end
    end
  end

  --- Shows structured compatibility losses and offers an explicit report export.
  local function show_information_loss(raw, operation)
    operation = operation or "import"
    local operation_title = operation:gsub("^%l", string.upper)
    if not raw or raw == "" then return end
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

  --- Shows the export compression choices shared by both export entrypoints.
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

  return {
    show_error = show_error,
    select_export_options = select_export_options,
    select_export_destination = select_export_destination,
    select_import_source = select_import_source,
    select_import_options = select_import_options,
    show_information_loss = show_information_loss,
    show_roundtrip_settings = show_roundtrip_settings,
  }
end

return Dialogs
