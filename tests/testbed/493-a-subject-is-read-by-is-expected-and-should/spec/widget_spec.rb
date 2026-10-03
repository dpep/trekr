RSpec.describe "Widget" do
  subject { 1 }

  it { is_expected.to eq(1) }

  context "when named" do
    subject(:widget) { 2 }

    it { should eq(2) }
  end

  context "when nothing reads it" do
    subject(:gadget) { 3 }

    it { expect(1).to eq(1) }
  end
end
