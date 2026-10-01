class WidgetSerializer < ActiveModel::Serializer
  include Stamped

  attributes :id, :title, :stamp
  has_one :owner

  def include_title?
    object.title.present?
  end

  def include_owner?
    object.owner
  end

  def include_ghost?
    false
  end
end
