require "tally"

RSpec.describe Mention do
  it "rewrites" do
    expect(Mention.rewrite("@a")).to eq "@x"
  end

  it "slugs" do
    expect(Mention.slug("A")).to eq "a"
  end
end
