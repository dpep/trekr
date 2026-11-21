require "oneline"

RSpec.describe Oneline do
  describe "#label" do
    it { expect(Oneline.new.label).to eq "l" }; it { expect(1).to eq 1 }
  end
end
