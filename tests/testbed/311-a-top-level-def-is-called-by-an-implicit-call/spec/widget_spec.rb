RSpec.describe Widget do
  it "is mocked" do
    mock_widget(:a)
  end

  it "is mocked again" do
    mock_widget(:b).label
  end

  it "has a label of its own" do
    Widget.new.stub_label
  end
end
