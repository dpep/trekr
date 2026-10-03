module Widgets
  RSpec.shared_examples "a measured widget" do
    it { expect(target).to be }
  end

  RSpec.describe "Widget" do
    let(:target) { 1 }
    let(:evaluated) { 2 }

    attr_accessor :recorded

    it_behaves_like "a measured widget"

    it { eval("expect(evaluated).to eq(2)") }
  end
end
