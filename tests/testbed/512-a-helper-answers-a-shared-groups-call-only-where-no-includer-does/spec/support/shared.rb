module NameHelper
  def thing_name
    "helper"
  end
end
RSpec.configure { |c| c.include NameHelper }

RSpec.shared_examples "uses thing name" do
  it { expect(thing_name).to be_a(String) }
end

RSpec.shared_examples "always named" do
  it { expect(thing_name).to be_a(String) }
end
