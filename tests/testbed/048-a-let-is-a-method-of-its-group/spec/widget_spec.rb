RSpec.describe Widget do
  subject(:widget) { described_class.new(size) }

  let(:size) { 3 }

  def helper
    size
  end

  context "when idle" do
    let(:size) { 0 }

    it { widget }
    it { helper }
    it { size }
  end

  context "when idle" do
    let(:speed) { 1 }
  end

  context "when busy" do
    it { speed }
    it { subject }
  end
end
