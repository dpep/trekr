require "support/gamma_helper"

RSpec.describe "Gamma" do
  include GammaHelper
  it { expect(gamma_name).to be }
  it_behaves_like "a gamma client"
end
