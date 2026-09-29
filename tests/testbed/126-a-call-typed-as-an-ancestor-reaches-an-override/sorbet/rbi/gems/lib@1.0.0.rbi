module Lib
  class Context
    sig { returns(T::Boolean) }
    def admin?; end

    sig { returns(String) }
    def locale; end
  end
end
