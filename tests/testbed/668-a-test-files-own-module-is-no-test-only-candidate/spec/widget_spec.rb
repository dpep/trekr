RSpec.describe "widgets" do
  it "builds one" do
    expect(WidgetHelpers.build).to be_a(Widget)
    expect(WidgetHelpers::Builder.new).to be_truthy
  end
end
