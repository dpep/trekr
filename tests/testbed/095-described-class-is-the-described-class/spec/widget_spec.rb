RSpec.describe Widget do
  it { described_class.build }
  it { described_class.new.save }

  describe "#save" do
    it { described_class.build }
  end

  context "with a let of that name" do
    let(:described_class) { [] }

    it { described_class.push(1) }
  end
end

RSpec.describe Helpers do
  it { described_class.format }
end

RSpec.describe "a widget" do
  it { described_class.build }
end
