module Helpers
  def sign_in(user)
  end
end

module Macros
  def with_widget
  end
end

RSpec.configure do |config|
  config.include Helpers, type: :model
  config.extend Macros
end
