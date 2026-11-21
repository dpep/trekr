RSpec.describe Plain do
  it "is labelled" do
    expect(Plain.new.plain_label).to eq "plain"
  end
end
