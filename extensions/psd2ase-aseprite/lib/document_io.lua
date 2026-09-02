local DocumentIO = {}

--- Creates the Aseprite document boundary for one initialized extension instance.
function DocumentIO.new(process)
  local state = {}

  --- Returns the native Save As suggestion for an imported PSD.
  local function suggested_output_path(input)
    return app.fs.joinPath(app.fs.filePath(input), app.fs.fileTitle(input) .. ".aseprite")
  end

  --- Reads the optional source active frame from a temporary conversion report.
  local function read_imported_active_frame(report_filename)
    local raw = process.read_file(report_filename)
    if raw == "" then
      return
    end
    local ok, frame_index = pcall(function()
      local report = json.decode(raw)
      return report and report.active_frame_index
    end)
    if not ok then
      return
    end
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

  return {
    suggested_output_path = suggested_output_path,
    read_imported_active_frame = read_imported_active_frame,
    current_frame_index = current_frame_index,
    open_as_unsaved_document = open_as_unsaved_document,
    create_export_snapshots = create_export_snapshots,
  }
end

return DocumentIO
