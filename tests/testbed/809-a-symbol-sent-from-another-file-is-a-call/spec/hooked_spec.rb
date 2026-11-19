RSpec.describe Hooked do
  it "saves" do
    Hooked.new.send(:on_save)
  end
end
