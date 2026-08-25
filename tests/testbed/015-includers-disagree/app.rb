module Shared
  def run
    prepare
  end
end

class Alpha
  include Shared

  def prepare
  end
end

class Beta
  include Shared

  def prepare
  end
end
