class Widget
  include ActiveModel::AttributeAssignment
  include Labelled

  def mode=(value)
    @overwrite = value == "overwrite"
  end

  def lonely
    :lonely
  end
end
