RSpec.describe "Widget" do
  let(:mode) { :default }
  subject { mode.to_s }

  context "when on" do
    let(:mode) { :on }
    it { is_expected.to eq("on") }
  end

  context "when off" do
    let(:mode) { :off }
    it { is_expected.to eq("off") }
  end
end

RSpec.describe "Gadget" do
  let(:mode) { :default }
  subject { mode.to_s }

  it { is_expected.to eq("default") }

  context "when on" do
    let(:mode) { :on }
    it { is_expected.to eq("on") }
  end
end
