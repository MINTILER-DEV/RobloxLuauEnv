print("server booting")

local shared = require(script.Parent.Shared)
local part = Instance.new("Part")
part.Name = shared.PartName
part.Position = Vector3.new(0, shared.Height, 0)
part.Size = Vector3.new(20, 1, 20)
part.CanCollide = true
part.Anchored = false
part.Parent = workspace

print("server created", part:GetFullName())
