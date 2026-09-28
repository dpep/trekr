RSpec.describe Widget do
  let(:widget) { Widget.new }
  let(:built) { described_class.new }
  let(:list) { [] }
  subject { described_class.new }

  it do
    widget.save
    built.save
    list.push(1)
    held = widget
    held.save
    expect(widget).to be_empty
    is_expected.to be_empty
  end

  context "when overridden" do
    let(:widget) { "text" }

    it { widget.save }
  end
end

RSpec.describe Widget do
  let(:widget) { Widget.new }

  before { widget.save }

  context "when a string" do
    let(:widget) { "text" }
  end
end
