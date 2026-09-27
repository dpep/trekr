module Remote
  class Account
    def email; end
  end
end

class Item
  def weight; end
end

module Billing
  class Account
    def email; end
  end

  class Item
    def price; end
  end

  class Invoice
    extend T::Sig

    sig { returns(Remote::Account) }
    def remote; end

    sig { returns(T.nilable(::Remote::Account)) }
    def rooted; end

    sig { returns(::Item) }
    def top_item; end
  end
end

def go
  invoice = Billing::Invoice.new
  invoice.remote.email
  invoice.rooted.email
  invoice.top_item.weight
end
