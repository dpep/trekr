require "sized"

RSpec.describe Sized do
  let(:size) { 1 }
  let(:sized) { Sized.new(size) }

  context "with a doubled size" do
    let(:size) { Pricing.doubled(3) }

    it "totals the doubled size" do
      expect(sized.total).to eq 6
    end
  end
end
