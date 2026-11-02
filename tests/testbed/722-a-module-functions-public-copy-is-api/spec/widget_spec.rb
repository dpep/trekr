RSpec.describe Widget do
  it "works" do
    Widget::Util.secure_compare("a", "b")
    Object.new.extend(Widget::Plain).send(:helper)
  end
end
