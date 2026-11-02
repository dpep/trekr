RSpec.describe Widget do
  it "works" do
    widget = Widget.new
    widget.twice
    widget.twice
    widget.once
    widget.live
    widget.run
    widget.send(:hidden)
    widget.many
    widget.many
    widget.many
    widget.many
    widget.many
    widget.many
    widget.many
    widget.many
    widget.many
    widget.many
    other = Other.new
    other.lone
    other.lone
    other.lone
    other.lone
    other.lone
    other.lone
    other.lone
    other.lone
    other.lone
    widget.ping
    widget.each
    other.ping
    other.ping
    other.ping
    other.ping
    other.ping
    other.ping
    other.ping
    other.ping
    WidgetHelpers.build_widget
  end
end
