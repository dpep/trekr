RSpec.shared_examples "a used thing" do
  it { expect(subject).to be }
end

RSpec.shared_examples "an unused thing" do
  let(:extra) { 1 }

  it { expect(extra).to eq(1) }
end

RSpec.shared_context "with tagged setup", :tagged do
  let(:tagged) { 2 }
end
