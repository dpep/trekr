RSpec.describe Doubled do
  RSpec.shared_examples "a doubler" do
    it { expect(Doubled.new.doubled_value).to eq 2 }
  end

  it "doubles" do
    expect(Doubled.new.doubled_value).to eq 2
  end
end
