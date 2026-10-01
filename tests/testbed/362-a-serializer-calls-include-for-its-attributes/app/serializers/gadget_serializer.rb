class GadgetSerializer < WidgetSerializer
  def include_id?
    object.id
  end
end
