require "minitest/autorun"

class ActiveSupport::TestCase
  setup do
    @n = Factory.build(2)
  end
end
