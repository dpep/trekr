RSpec.describe Widget do
  it "works" do
    widget = Widget.new
    widget.reset
    widget.rebuild(true)
    widget.flush
    widget.expand
    Widget.const_get(:Store).new.put
  end
end
