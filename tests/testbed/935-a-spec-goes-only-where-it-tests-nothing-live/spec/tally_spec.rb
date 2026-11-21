require "tally"

RSpec.describe Tally do
  it "totals" do
    expect(Tally.new([1, 2]).total).to eq 6
  end
end
