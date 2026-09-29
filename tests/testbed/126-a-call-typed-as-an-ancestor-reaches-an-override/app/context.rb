module App
  class Context < Lib::Context
    def admin?
      true
    end

    def locale
      "en"
    end

    def audit
    end
  end
end

class Account
  def admin?
    false
  end
end
