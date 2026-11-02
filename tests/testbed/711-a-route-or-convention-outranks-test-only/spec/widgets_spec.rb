RSpec.describe WidgetsController do
  it "lists" do
    WidgetsController.new.index
  end
end

RSpec.describe WidgetPolicy do
  it "allows" do
    WidgetPolicy.new.publish?
    WidgetPolicy.new.publish?
    WidgetPolicy.new.archive?
  end
end
