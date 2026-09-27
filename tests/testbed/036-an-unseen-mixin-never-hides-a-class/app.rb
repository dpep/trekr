class Base
  include Unseen::Helpers
end

class Child < Base
  def show
    super
  end
end

class Stranger
  def show
  end
end

module Maybe
  def show
  end
end

class Gadget < Unseen::Base
  def render
    super
  end
end

class Faraway
  def render
  end
end
