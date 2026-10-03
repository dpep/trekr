RSpec.describe Shop::Widget do
  let(:widget) { described_class.new }

  it "builds" do
    expect(widget).to be_truthy
  end
end
