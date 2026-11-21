RSpec.describe Clocked do
  class FakeClock
    def now
      0
    end
  end

  it "is clocked" do
    expect(Clocked.new.clocked_value).to eq 2
  end
end
