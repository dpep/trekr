RSpec.describe Relayed do
  include PriceRelay

  it "formats" do
    expect(format_price(Relayed.new.relayed_value)).to eq "$2"
  end
end
