module GammaHelper
  def gamma_name
    :gamma
  end

  def gamma_size
    1
  end
end

module DeltaHelper
  def delta_name
    :delta
  end
end

RSpec.configure do |config|
  config.include DeltaHelper
end

RSpec.shared_examples "a gamma client" do
  it { expect(gamma_size).to eq(1) }
  it { expect(delta_name).to be }
end
