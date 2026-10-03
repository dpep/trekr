RSpec.describe "Widget" do
  let(:current_widget) { 1 }
  let(:item_first) { 2 }
  let(:plain) { 3 }

  before { send("item_#{position}") }

  it { expect(true).to be(true) }
end

RSpec.describe "Gadget" do
  let(:quiet) { 4 }

  with_gadget_defaults

  it { expect(true).to be(true) }
end
