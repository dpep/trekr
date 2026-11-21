RSpec.describe Checkout do
  it_behaves_like "a doubler"

  it "totals" do
    expect(FakeClock.new.now).to eq LIMIT - 2
    expect(Checkout.new.total).to eq "$3"
  end
end
