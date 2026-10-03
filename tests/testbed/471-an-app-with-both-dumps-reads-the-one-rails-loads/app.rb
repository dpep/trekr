class Widget < ActiveRecord::Base
  def shout
    code.upcase
  end
end
