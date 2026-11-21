RSpec.describe Configured do
  it "formats" do
    expect(format_price(Configured.new.configured_value)).to eq "$2"
  end
end
