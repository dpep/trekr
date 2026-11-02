RSpec.describe Widget do
  it "works" do
    matcher = Widget::Matcher.new
    matcher.of(1)
    matcher.reset
    Widget::Matcher::Step.new.advance
    Widget::Hidden::Shown.new.show
    Widget::Legacy.new.run
  end
end
