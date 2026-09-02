local root = assert(app.params["extensionRoot"], "extensionRoot script parameter is required")

--- Loads one extension module from a checkout or unpacked extension directory.
local function load_module(filename)
  local path = app.fs.joinPath(root, "lib", filename)
  local chunk, load_error = loadfile(path)
  assert(chunk, tostring(load_error))
  return assert(chunk())
end

local entry_chunk = assert(loadfile(app.fs.joinPath(root, "psd2ase.lua")))
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
assert(type(dialogs.select_export_options) == "function")
assert(type(documents.create_export_snapshots) == "function")
assert(type(workflows.import_document) == "function")
assert(type(workflows.save_photoshop_document) == "function")

local import_arguments = process.build_arguments("converter", "input.psd", "output.aseprite", {
  report = "report.json",
  overwrite = true,
  preserve_photoshop_metadata = true,
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
assert(import_text:find("--jitter-mode\0repair", 1, true))
assert(import_text:find("--uncertain-layers\0group", 1, true))

local export_arguments = process.build_export_arguments(
  "converter",
  "input.aseprite",
  "output.psd",
  "composite.aseprite",
  "report.json",
  3,
  "zip-prediction",
  false)
local export_text = table.concat(export_arguments, "\0")
assert(export_text:find("--active-frame-index\0" .. "3", 1, true))
assert(export_text:find("--compression\0zip-prediction", 1, true))
assert(export_text:find("--roundtrip-metadata\0off", 1, true))

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

print("psd2ase Lua module smoke test passed")
