class WidgetSerializer < ActiveModel::Serializer
  attributes :id, :title

  def include_title?
    true
  end
end
