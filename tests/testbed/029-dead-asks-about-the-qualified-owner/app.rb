module Alpha
  module Helpers
    def shout
      "hey"
    end
  end
end

class User
  include Alpha::Helpers

  def go
    shout
  end
end
