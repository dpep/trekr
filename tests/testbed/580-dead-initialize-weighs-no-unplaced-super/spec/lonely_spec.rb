RSpec.describe "construction" do
  it "stubs" do
    allow(Built).to receive(:new)
    allow_any_instance_of(Built).to receive(:initialize)
  end
end
