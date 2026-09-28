RSpec.describe Widget do
  it { is_expected.to be_empty }
  it { subject.save }

  describe "#save" do
    it { subject.save }
  end

  context "with a subject" do
    subject { [] }

    it { subject.push(1) }
  end
end

RSpec.describe "a widget" do
  it { subject.save }
end
