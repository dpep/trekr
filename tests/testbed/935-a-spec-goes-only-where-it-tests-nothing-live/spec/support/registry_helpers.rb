module RegistryHelpers
  def with_registry
    before { Setup.registry! }
  end
end

RSpec.configure do |config|
  config.extend RegistryHelpers
end
