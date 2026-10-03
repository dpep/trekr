RSpec.describe "Gadget" do
  include_context "with a server"

  let(:mode) { :on }

  it_behaves_like "a defaulted thing"

  it { expect(serving).to be }
end
