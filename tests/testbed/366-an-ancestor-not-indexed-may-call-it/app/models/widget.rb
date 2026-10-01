class Widget
  include Vendor::Trackable

  def build_resource(attrs)
    super
    attrs
  end

  def lonely
    :lonely
  end
end
