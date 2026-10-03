RSpec.describe "Widget" do
  let(:label) { "x" }
  let(:unused) { 1 }
  let(:hooked) { 2 }
  let(:outer_only) { 3 }
  let!(:eager) { 4 }
  let(:overridden) { 5 }

  before { hooked }

  def helper_used
    outer_only
  end

  def helper_unused
    1
  end

  it_behaves_like "a labelled widget"

  context "nested" do
    let(:overridden) { super() + 1 }
    let(:read_by_outer_hook) { 6 }

    it { expect(helper_used).to eq(3) }
    it { expect(overridden).to eq(6) }
  end
end

RSpec.describe "Gadget" do
  before { read_by_outer_hook }

  context "inner" do
    let(:read_by_outer_hook) { 7 }

    it { expect(true).to be(true) }
  end
end
