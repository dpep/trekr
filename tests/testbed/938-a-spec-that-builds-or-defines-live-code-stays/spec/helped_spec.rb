RSpec.describe Helped do
  include PriceHelper

  it "formats" do
    expect(format_price(Helped.new.helped_value)).to eq "$2"
  end
end
