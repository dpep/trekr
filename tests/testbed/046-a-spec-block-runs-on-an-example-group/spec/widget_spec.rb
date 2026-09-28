RSpec.describe Widget do
  let(:size) { 3 }
  before { described_class }

  context "when idle" do
    let(:speed) { is_expected }
  end

  class Helper
    def assist
    end
  end

  it { Helper.new.assist }
end
