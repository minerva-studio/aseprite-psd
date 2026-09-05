local root = assert(app.params["extensionRoot"], "extensionRoot script parameter is required")

--- Loads one extension module from a checkout or unpacked extension directory.
local function load_module(filename)
  local path = app.fs.joinPath(root, "lib", filename)
  local chunk, load_error = loadfile(path)
  assert(chunk, tostring(load_error))
  return assert(chunk())
end

local entry_chunk = assert(loadfile(app.fs.joinPath(root, "aseprite-psd.lua")))
entry_chunk()
assert(type(init) == "function", "entry script must define init")

local Process = load_module("process.lua")
local Dialogs = load_module("dialogs.lua")
local DocumentIO = load_module("document_io.lua")
local Workflows = load_module("workflows.lua")
local process = Process.new({ path = root })
local dialogs = Dialogs.new(process)
local documents = DocumentIO.new(process)
local workflows = Workflows.new(process, dialogs, documents)

assert(type(process.build_arguments) == "function")
assert(type(process.build_export_arguments) == "function")
assert(type(process.with_temp_files) == "function")
assert(type(dialogs.select_import_options) == "function")
assert(dialogs.default_import_options().layer_association == "auto")
assert(dialogs.default_import_options().frame_source == "auto")
assert(dialogs.default_import_options().association_strategy == "conservative")
assert(dialogs.default_import_options().use_roundtrip_metadata == true)
assert(type(dialogs.select_roundtrip_recovery) == "function")
assert(type(dialogs.select_export_options) == "function")
assert(type(documents.create_export_snapshots) == "function")
assert(type(workflows.import_document) == "function")
assert(type(workflows.load_photoshop_document) == "function")
assert(type(workflows.save_photoshop_document) == "function")

local import_arguments = process.build_arguments("converter", "input.psd", "output.aseprite", {
  report = "report.json",
  overwrite = true,
  preserve_photoshop_metadata = true,
  frame_source = "top-level",
  link_identical_cels = true,
  layer_association = "auto",
  association_strategy = "conservative",
  z_order = "stable",
  stable_order = "consensus",
  uncertain_layers = "group",
  jitter_mode = "repair",
  jitter_kind = "all",
  jitter_profile = "balanced",
})
local import_text = table.concat(import_arguments, "\0")
assert(import_text:find("--linked-cels\0identical", 1, true))
assert(import_text:find("--frame-source\0top-level", 1, true))
assert(import_text:find("--jitter-mode\0repair", 1, true))
assert(import_text:find("--uncertain-layers\0group", 1, true))

local roundtrip_arguments = process.build_arguments("converter", "input.psd", "output.aseprite", {
  report = "report.json",
  overwrite = true,
  layer_association = "roundtrip",
  jitter_mode = "off",
})
local roundtrip_text = table.concat(roundtrip_arguments, "\0")
assert(roundtrip_text:find("--layer-association\0roundtrip", 1, true))

local automatic_roundtrip_arguments = process.build_arguments("converter", "input.psd", "output.aseprite", {
  report = "report.json",
  overwrite = true,
  layer_association = "auto",
  use_roundtrip_metadata = true,
  association_strategy = "compact",
  z_order = "stable",
  stable_order = "consensus",
  jitter_mode = "off",
})
local automatic_roundtrip_text = table.concat(automatic_roundtrip_arguments, "\0")
assert(automatic_roundtrip_text:find("--layer-association\0roundtrip", 1, true))
assert(not automatic_roundtrip_text:find("--association-strategy", 1, true))

local automatic_arguments = process.build_arguments("converter", "input.psd", "output.aseprite", {
  report = "report.json",
  overwrite = true,
  layer_association = "auto",
  use_roundtrip_metadata = false,
  association_strategy = "compact",
  z_order = "stable",
  stable_order = "consensus",
  jitter_mode = "off",
})
local automatic_text = table.concat(automatic_arguments, "\0")
assert(automatic_text:find("--layer-association\0auto", 1, true))
assert(automatic_text:find("--association-strategy\0compact", 1, true))
assert(automatic_text:find("--z-order\0stable", 1, true))
assert(automatic_text:find("--stable-order\0consensus", 1, true))

local feature_arguments = process.build_arguments("converter", "input.psd", "output.aseprite", {
  frame_source = "layer-depth:1",
  layer_association = "auto",
  association_strategy = "Feature tracks",
  z_order = "stable",
  stable_order = "consensus",
  jitter_mode = "off",
})
local feature_text = table.concat(feature_arguments, "\0")
assert(feature_text:find("--frame-source\0layer-depth:1", 1, true))
assert(feature_text:find("--association-strategy\0feature", 1, true))
assert(not feature_text:find("--uncertain-layers", 1, true))

local preserve_arguments = process.build_arguments("converter", "input.psd", "output.aseprite", {
  report = "report.json",
  overwrite = true,
  layer_association = "preserve",
  use_roundtrip_metadata = true,
  association_strategy = "conservative",
  jitter_mode = "off",
})
local preserve_text = table.concat(preserve_arguments, "\0")
assert(not preserve_text:find("--layer-association", 1, true))
assert(not preserve_text:find("--association-strategy", 1, true))

local frame_report = process.temporary_path("json")
process.write_file(frame_report, '{"active_frame_index":2}')
assert(documents.read_imported_active_frame(frame_report) == 2)
process.write_file(frame_report, '{"active_frame_index":"2"}')
assert(documents.read_imported_active_frame(frame_report) == nil)
process.remove_file(frame_report)

local export_arguments = process.build_export_arguments(
  "converter",
  "input.aseprite",
  "output.psd",
  "composite.aseprite",
  "report.json",
  3,
  false,
  false)
local export_text = table.concat(export_arguments, "\0")
assert(export_text:find("--active-frame-index\0" .. "3", 1, true))
assert(not export_text:find("--compression", 1, true))
assert(export_text:find("--roundtrip-metadata\0off", 1, true))
assert(export_text:find("--empty-layers\0omit", 1, true))

local default_export_arguments = process.build_export_arguments(
  "aseprite-psd.exe",
  "source.aseprite",
  "composite.aseprite",
  "output.psd",
  nil,
  nil,
  true,
  false)
local default_export_text = table.concat(default_export_arguments, "\0")
assert(default_export_text:find("--empty-layers\0omit", 1, true))

local temporary_path
local result = process.with_temp_files({"smoke"}, function(path)
  temporary_path = path
  process.write_file(path, "smoke")
  return "ok"
end)
assert(result == "ok")
assert(not app.fs.isFile(temporary_path), "successful workflow must clean temporary files")

local failed_path
local success = pcall(function()
  process.with_temp_files({"smoke"}, function(path)
    failed_path = path
    error("expected smoke failure")
  end)
end)
assert(not success, "workflow failure must propagate")
assert(not app.fs.isFile(failed_path), "failed workflow must clean temporary files")

print("aseprite-psd Lua module smoke test passed")
