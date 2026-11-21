RSpec.describe Konst do
  it "formats" do
    expect(Formats::MONEY.call(Konst.new.konst_value)).to eq "2.00"
  end
end
