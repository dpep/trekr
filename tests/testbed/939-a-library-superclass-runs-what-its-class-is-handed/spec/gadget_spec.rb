RSpec.describe Gadget do
  it "is labelled" do
    expect(Gadget.new.label).to eq "gadget"
  end
end
