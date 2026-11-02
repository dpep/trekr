class Gadget
  def own; end
end

Gizmo = Gadget
Gizmo.class_eval do
  def through_alias; end
end

Gadget.new.through_alias
