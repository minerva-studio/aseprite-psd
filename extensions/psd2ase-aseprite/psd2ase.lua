local COMMAND_ID = "Psd2aseImport"

--- Loads one extension module relative to the installed extension directory.
local function load_module(plugin, filename)
  local path = app.fs.joinPath(plugin.path, "lib", filename)
  local chunk, load_error = loadfile(path)
  if not chunk then
    error("Could not load psd2ase module " .. path .. ": " .. tostring(load_error))
  end
  local ok, module = pcall(chunk)
  if not ok then
    error("Could not initialize psd2ase module " .. path .. ": " .. tostring(module))
  end
  return module
end

--- Registers the PSD import/export commands and file format for Aseprite.
function init(plugin)
  local Process = load_module(plugin, "process.lua")
  local Dialogs = load_module(plugin, "dialogs.lua")
  local DocumentIO = load_module(plugin, "document_io.lua")
  local Workflows = load_module(plugin, "workflows.lua")

  local process = Process.new(plugin)
  local dialogs = Dialogs.new(process)
  local documents = DocumentIO.new(process)
  local workflows = Workflows.new(process, dialogs, documents)

  if plugin.preferences.embed_roundtrip_metadata == nil then
    plugin.preferences.embed_roundtrip_metadata = true
  end

  plugin:newCommand{
    id=COMMAND_ID,
    title="Import PSD to Aseprite...",
    group="file_import",
    onclick=function()
      if not process.binary then
        dialogs.show_error("PSD to Aseprite", "This extension has no binary for the current platform.")
        return
      end
      workflows.import_from_menu()
    end,
  }
  plugin:newCommand{
    id="Psd2aseExport",
    title="Export PSD/PSB...",
    group="file_export",
    onclick=function()
      workflows.export_from_menu(plugin)
    end,
  }
  plugin:newCommand{
    id="Psd2aseSettings",
    title="PSD to Aseprite Settings...",
    group="file_export",
    onclick=function()
      dialogs.show_roundtrip_settings(plugin)
    end,
  }
  plugin:newFileFormat{
    name="Photoshop Document (PSD/PSB)",
    extensions={"psd", "psb"},
    binary=true,
    onsave=function(ev)
      return workflows.save_photoshop_document(ev, plugin)
    end,
    onload=function(ev)
      if not process.binary then
        error("This extension has no converter for the current platform.")
      end
      local sprite, report = workflows.import_document(ev.filename)
      if sprite then
        dialogs.show_information_loss(report)
      end
      return sprite
    end,
  }
  if not process.binary or not app.fs.isFile(process.binary) then
    print("psd2ase: no supported bundled executable was found")
  end
end
