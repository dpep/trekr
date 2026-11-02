RSpec.describe Widget do
  it "works" do
    widget = Widget.new
    widget.expand
    widget.reset
    widget.flush
    widget.rebuild
    widget._compact
    Widget::Cache.new.fetch
  end
end
