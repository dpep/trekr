require "labeled"

RSpec.describe Labeled do
  with_registry

  it "labels" do
    expect(Labeled.new.label).to eq "l"
  end
end
