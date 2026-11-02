RSpec.describe Widget::Timestamp do
  it "converts" do
    Widget::Timestamp.new.to_time
  end
end
